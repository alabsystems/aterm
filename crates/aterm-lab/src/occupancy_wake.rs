// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `occupancy-wake-claude`: a declared wake told at a background break, on a real Claude Code
//! (the slice-2 plan's 2.10).
//!
//! The agent declares a wake on a flag file under its scratch home (`aterm ctl @self wake`), then
//! starts a background workflow and waits on it. The lab touches the flag. Under the scratch
//! home's `occupancy = "prompt"` the harness types the wake's one closed line at the break (or,
//! where the policy acts at a point, rides its act): never into a live turn, never the wake's
//! label or path, and its `waits` reads `told` after. The lab's `[harness]` keys turn the
//! policy's own continuations off, so the wake's line is the one line the harness types.
//!
//! `occupancy-wake-standin` ([`STANDIN`]) is the same play and the same judge with the lab's own
//! stand-in as the agent ([`crate::occupancy_standin`]): the harness's side with no real agent and
//! no API, which is all it says.

use std::path::Path;

use aterm_agent::harness::upgrade::HARNESS_MARK;
use aterm_agent::supervise::facts::FactKind;
use aterm_agent::supervise::occupancy::door::INTENT_RULE;
use aterm_agent::supervise::occupancy::due::K_LOOKS;
use aterm_agent::supervise::occupancy::reading::LOOK_EVERY_MS;
use aterm_agent::supervise::occupancy::rules::WOULD_RULE;
use aterm_agent::supervise::occupancy::templates;
use aterm_agent::supervise::occupancy::wakes_seen::{AtomKind, Declarer, WAKE_RULE, WakeDoor};
use aterm_phase::phase::{Phase, background_wait, worker_phase};

use crate::pyjson::{COMPACT, J};
use crate::record::{Frame, rec_s};
use crate::scn::{Ctx, ScnErr};
use crate::stop;
use crate::util::now;

/// The scenario's name (`lab scn run`; its runs `<name>-NN`, its corpus tape `<name>`).
pub const NAME: &str = "occupancy-wake-claude";

/// The scenario with the lab's stand-in in the agent's place.
pub const STANDIN: &str = "occupancy-wake-standin";

/// Its `[harness]` lines, under the lab's root `[harness]` table: `prompt` under Cruise, and no
/// continuation of the policy's own (`continue = false` holds Cruise's carry at a break too), so
/// the wake's line is the one line the harness types.
pub const HARNESS_KEYS: &str =
    "occupancy = \"prompt\"\nautopilot = \"cruise\"\ncontinue = false\nhuman_grace_s = 5";

/// The wake's label: a plant that must never be typed or recorded as a line.
pub const LABEL: &str = "lab-wake-plant";

/// The flag's file name, in the run's folder under the scratch home's `scratch.noindex`.
pub const FLAG: &str = "flag";

/// What a workflow step's subagent is asked under.
pub const STEP_MARK: &str = "occupancy-wake-lab-step";

/// The workflow the agent starts (the `Workflow` tool's `script`).
pub const WORKFLOW: &str = "export const meta = { name: 'occupancy-wake-lab', description: 'Two sleeps the wake lab waits on' }\n\
                            await parallel([1, 2].map((n) => () => agent(`occupancy-wake-lab-step ${n}: run sleep 900 in Bash, then say slept.`)))\n";

/// The task the lab types: the stub's tool mode starts at a message holding `toolstart`.
pub const TURN: &str = "toolstart: declare a wake on the lab's flag file, then start a background \
                        workflow of two sleep 900 steps and wait on it";

/// The main thread's word once the workflow runs: a plain status, no question to the person.
pub const STARTED: &str = "Declared wake #1 on the lab's flag and started the occupancy-wake-lab \
                           workflow: its two sleep steps run in the background, and I am waiting \
                           on them.";

/// The main thread's answer to the wake's line.
pub const ANSWERED: &str = "Wake #1 holds: the lab's flag is there. The workflow's two sleep \
                            steps still run, and I keep waiting on them.";

const SLEPT: &str = "slept";

const BASH_TIMEOUT_MS: i64 = 600_000;

const WORKFLOW_BOX: &str = aterm_phase::anchor!("box.workflow");

const WAITS_WITHIN_S: f64 = 120.0;

const ANSWERED_WITHIN_S: f64 = 60.0;

/// The run's folder under the scratch home (`scratch.noindex/<run>`), the flag's.
#[must_use]
pub fn flag_dir(run: &str) -> String {
    format!("scratch.noindex/{run}")
}

/// The Bash the agent declares its wake with: the flag, `~`-rooted, under `scratch.noindex`.
#[must_use]
pub fn declare_command(run: &str) -> String {
    format!(
        "aterm ctl @self wake label={LABEL} when='file:~/{}/{FLAG}'",
        flag_dir(run)
    )
}

/// How long the line may take once the flag is there: a look for the file's level read (a watch
/// sees it sooner), K looks (past which an untold wake is an Entry instead), and a minute more:
/// a hang detector, never a latency budget.
#[must_use]
pub fn told_within_s() -> f64 {
    let ms = LOOK_EVERY_MS
        .saturating_mul(K_LOOKS.saturating_add(1))
        .saturating_add(60_000);
    std::time::Duration::from_millis(u64::try_from(ms).unwrap_or(0)).as_secs_f64()
}

/// The stub's tool script, for the run named `run`.
#[must_use]
pub fn script(run: &str) -> J {
    let step = |pairs: Vec<(&str, J)>| {
        let mut o = J::obj([("when", J::s(STEP_MARK))]);
        for (k, v) in pairs {
            o.set(k, v);
        }
        o
    };
    J::Arr(vec![
        J::obj([
            ("name", J::s("Bash")),
            (
                "input",
                J::obj([
                    ("command", J::s(declare_command(run))),
                    ("description", J::s("Declare the lab's wake")),
                ]),
            ),
        ]),
        J::obj([
            ("name", J::s("Workflow")),
            ("input", J::obj([("script", J::s(WORKFLOW))])),
        ]),
        J::obj([("text", J::s(STARTED))]),
        J::obj([("text", J::s(ANSWERED))]),
        step(vec![
            ("name", J::s("Bash")),
            (
                "input",
                J::obj([
                    ("command", J::s("sleep 900")),
                    ("description", J::s("Sleep for the wake lab")),
                    ("timeout", J::Int(BASH_TIMEOUT_MS)),
                ]),
            ),
        ]),
        step(vec![("text", J::s(SLEPT))]),
    ])
}

fn allowed() -> J {
    J::obj([(
        "permissions",
        J::obj([(
            "allow",
            J::Arr(
                [
                    "Workflow",
                    "Bash(sleep:*)",
                    "Bash(sleep *)",
                    "Bash(aterm ctl @self wake:*)",
                ]
                .into_iter()
                .map(J::s)
                .collect(),
            ),
        )]),
    )])
}

/// Whether `rows` draw the agent waiting on a workflow of its own.
#[must_use]
pub fn workflow_waits(rows: &[String]) -> bool {
    matches!(
        background_wait(rows),
        Some("waiting for a dynamic workflow" | "a workflow running")
    )
}

/// The door `word` names, of the two a wake's line is typed at (`await wake`'s answer types
/// nothing).
fn typed_door(word: &str) -> Option<WakeDoor> {
    [WakeDoor::Break, WakeDoor::Point]
        .into_iter()
        .find(|door| door.word() == word)
}

/// Whether journal `line` says the wake's line typed at `door`: alone at a break
/// (`OCCUPANCY … rule=wake@v1 … door=break typed`), or riding the act the policy typed at an idle
/// point (`FACT … kind=wake … door=point typed`).
fn typed_at(door: WakeDoor, line: &str) -> bool {
    match door {
        WakeDoor::Break => {
            line.starts_with("OCCUPANCY ")
                && word(line, "rule") == Some(WAKE_RULE)
                && line.ends_with(" door=break typed")
        }
        WakeDoor::Point => {
            line.starts_with("FACT ")
                && word(line, "kind") == Some(FactKind::Wake.word())
                && line.ends_with(" door=point typed")
        }
        WakeDoor::Await => false,
    }
}

/// Whether journal `line` says the wake's line typed, at either door.
#[must_use]
pub fn typed_line(line: &str) -> bool {
    typed_at(WakeDoor::Break, line) || typed_at(WakeDoor::Point, line)
}

/// Whether `rows`, the screen the line was typed into, is `door`'s: a break draws the agent's own
/// background work and nothing else live; a point reads idle. Anything else is a live turn.
fn drawn_at(door: WakeDoor, rows: &[String]) -> bool {
    match door {
        WakeDoor::Break => background_wait(rows).is_some(),
        WakeDoor::Point => worker_phase(rows) == Phase::Idle,
        WakeDoor::Await => false,
    }
}

/// What the judge reads of one run, all of it after the task was typed.
#[derive(Debug, Clone, Copy)]
pub struct Recording<'a> {
    /// The session's id.
    pub sid: &'a str,
    /// The run's name, whose folder holds the flag.
    pub run: &'a str,
    /// The session's `status` reply.
    pub status: &'a str,
    /// Its `waits` reply once the workflow waited, before the flag was touched.
    pub waits_before: &'a str,
    /// Its `waits` reply after the line.
    pub waits_after: &'a str,
    /// The session's journal lines, oldest first.
    pub journal: &'a [String],
    /// The session's ledger text.
    pub ledger: &'a str,
    /// The instance's `messages tag=harness` reply.
    pub messages: &'a str,
    /// The last screen read before the line reached it: the one it was typed into.
    pub at_write: Option<&'a [String]>,
    /// The screens read after the line was typed, oldest first.
    pub after: &'a [Vec<String>],
}

/// One ledger row of the session's, as the judge reads it.
#[derive(Debug, Clone)]
struct Row {
    rule: String,
    decision: String,
    command: String,
    reason: String,
}

fn rows_of(ledger: &str, sid: &str) -> Vec<Row> {
    let want = sid.trim_start_matches('@');
    ledger
        .lines()
        .filter_map(|l| J::parse(l).ok())
        .filter(|r| rec_s(r, "sid") == want)
        .map(|r| Row {
            rule: rec_s(&r, "rule_id"),
            decision: rec_s(&r, "decision"),
            command: rec_s(&r, "command"),
            reason: rec_s(&r, "reason"),
        })
        .collect()
}

fn word<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.split(' ')
        .find_map(|w| w.strip_prefix(key)?.strip_prefix('='))
}

fn record_key(sid: &str) -> String {
    format!("harness.supervisor.{}", sid.trim_start_matches('@'))
}

fn titles_keyed(messages: &str, key: &str) -> Vec<String> {
    let field = |row: &str, name: &str| {
        row.split(' ')
            .find_map(|w| w.strip_prefix(name))
            .map(crate::forever::pct_decode)
    };
    messages
        .lines()
        .filter(|row| row.starts_with("message "))
        .filter(|row| field(row, "key=").as_deref() == Some(key))
        .filter_map(|row| field(row, "title="))
        .collect()
}

/// The state `waits` gives wake 1, from its rows (with or without the client's `OK <n>` head).
fn state_of_first(waits: &str) -> Option<&str> {
    waits
        .lines()
        .find_map(|row| row.strip_prefix("#1 "))
        .and_then(|rest| rest.split(' ').next())
}

/// Wake 1's line as the closed template makes it at any local clock (or none): its whole line at
/// a break, its fact alone riding a point's act. `None` for any other text.
fn closed_line(command: &str, door: WakeDoor) -> Option<templates::FactLine> {
    (0..24u8)
        .flat_map(|h| (0..60u8).map(move |m| Some((h, m))))
        .chain([None])
        .map(|since| templates::wake(1, Declarer::Agent, &[AtomKind::File], since, None))
        .find(|line| match door {
            WakeDoor::Break => line.text() == command,
            WakeDoor::Point => line.fact() == command,
            WakeDoor::Await => false,
        })
}

/// THE SCENARIO'S VERDICT on `r`: one line per shortfall found, none for a pass.
#[must_use]
pub fn judge(r: &Recording<'_>) -> Vec<String> {
    let mut fail = Vec::new();
    if !r.status.split_whitespace().any(|w| w == "occupancy=prompt") {
        fail.push("status does not say occupancy=prompt".to_string());
    }
    if state_of_first(r.waits_before) != Some("pending") {
        fail.push(format!(
            "wake #1 was not pending before the flag: {:?}",
            r.waits_before
        ));
    }
    if state_of_first(r.waits_after) != Some("told") {
        fail.push(format!(
            "wake #1 is not told after the line: {:?}",
            r.waits_after
        ));
    }
    let fact_rule = FactKind::Wake.rule();
    let rows = rows_of(r.ledger, r.sid);
    let would = rows.iter().filter(|w| w.rule == WOULD_RULE).count();
    if would > 0 {
        fail.push(format!("{would} {WOULD_RULE} row(s) under prompt"));
    }
    let skipped = rows
        .iter()
        .filter(|w| w.rule == fact_rule && w.decision == "skipped")
        .count();
    if skipped > 0 {
        fail.push(format!("{skipped} {fact_rule} skipped row(s)"));
    }
    let plant = flag_dir(r.run);
    let planted = |text: &str| text.contains(LABEL) || text.contains(&plant);
    if rows
        .iter()
        .filter(|w| {
            w.decision == "typed" || [fact_rule, INTENT_RULE, WOULD_RULE].contains(&w.rule.as_str())
        })
        .any(|w| planted(&w.command) || planted(&w.reason))
        || titles_keyed(r.messages, &record_key(r.sid))
            .iter()
            .any(|t| planted(t))
    {
        fail.push("the wake's label or path reached a line".to_string());
    }
    let typed: Vec<(usize, &Row)> = rows
        .iter()
        .enumerate()
        .filter(|(_, w)| w.rule == fact_rule && w.decision == "typed")
        .collect();
    let intents: Vec<(usize, &Row)> = rows
        .iter()
        .enumerate()
        .filter(|(_, w)| {
            w.rule == INTENT_RULE && word(&w.reason, "kind") == Some(FactKind::Wake.word())
        })
        .collect();
    let mut told = None;
    match (typed.as_slice(), intents.as_slice()) {
        ([(at, row)], [(intent_at, intent)]) => {
            let door = word(&row.reason, "door").and_then(typed_door);
            let episode =
                word(&row.reason, "episode").filter(|key| u64::from_str_radix(key, 16).is_ok());
            if word(&row.reason, "rule") != Some(WAKE_RULE) || word(&row.reason, "ord") != Some("1")
            {
                fail.push(format!(
                    "the typed row is no {WAKE_RULE} #1 row: {}",
                    row.reason
                ));
            }
            if intent.decision != "waiting" || word(&intent.reason, "ord") != Some("1") {
                fail.push("the intent row is no waiting wake #1 row".to_string());
            }
            if intent_at > at {
                fail.push("the intent row follows the typed row".to_string());
            }
            if word(&intent.reason, "door") != word(&row.reason, "door") {
                fail.push("the intent and the typed row name two doors".to_string());
            }
            match (door, episode) {
                (Some(door), Some(episode)) => {
                    if word(&intent.reason, "episode") != Some(episode) {
                        fail.push("the intent and the typed row are two episodes".to_string());
                    }
                    match closed_line(&row.command, door) {
                        Some(line) => told = Some((door, episode, line)),
                        None => fail.push(format!(
                            "the typed row's line is not wake #1's closed line: {:?}",
                            row.command
                        )),
                    }
                }
                (None, _) => fail.push("the typed row names no door".to_string()),
                (_, None) => fail.push("the typed row has no valid episode".to_string()),
            }
        }
        _ => fail.push(format!(
            "{} {fact_rule} typed row(s) and {} wake {INTENT_RULE} row(s), not one of each",
            typed.len(),
            intents.len()
        )),
    }
    let Some((door, episode, line)) = told else {
        return fail;
    };
    if !titles_keyed(r.messages, &record_key(r.sid))
        .iter()
        .any(|title| title.contains(line.fact()))
    {
        fail.push(format!(
            "no {} record of the line in Messages",
            record_key(r.sid)
        ));
    }
    let typed_lines: Vec<(usize, &String)> = r
        .journal
        .iter()
        .enumerate()
        .filter(|(_, l)| typed_line(l))
        .collect();
    match typed_lines.as_slice() {
        [(at, one)] if typed_at(door, one) => {
            if word(one, "episode") != Some(episode) {
                fail.push("the journal's typed line is not the ledger's episode".to_string());
            }
            let opened = |l: &String| l.starts_with("OCCUPANCY ") && l.contains(" hold open");
            if door == WakeDoor::Break && !r.journal[..*at].iter().any(opened) {
                fail.push("no hold open before the break's line".to_string());
            }
        }
        [_] => fail.push("the journal's typed line names another door".to_string()),
        many => fail.push(format!(
            "{} journal line(s) say the wake typed, not one",
            many.len()
        )),
    }
    match r.at_write {
        Some(rows) if drawn_at(door, rows) => {}
        Some(_) => fail.push(format!(
            "the line was typed into a live turn, not at a {}",
            door.word()
        )),
        None => fail.push("no screen was read before the line".to_string()),
    }
    if door == WakeDoor::Break && !r.after.last().is_some_and(|rows| workflow_waits(rows)) {
        fail.push("the last screen after the break's line draws no running workflow".to_string());
    }
    fail
}

/// The screen the line was typed into: the last read before `typed_seen` (the journal's typed line
/// seen) that draws no harness line yet, since one that does is already past the write. A read is
/// a change of the screen, so it stands until the next however long ago it came: a screen that sat
/// still from before the flag's write to the line has no read in between.
#[must_use]
pub fn before_line(frames: &[Frame], typed_seen: f64) -> Option<Vec<String>> {
    frames
        .iter()
        .rev()
        .find(|f| f.wall < typed_seen && !f.rows.iter().any(|row| row.contains(HARNESS_MARK)))
        .map(|f| f.rows.clone())
}

/// The first journal line from `from` that says the wake's line typed, within `timeout`
/// seconds: its index.
fn typed_after(ctx: &mut Ctx, from: usize, timeout: f64) -> Result<Option<usize>, ScnErr> {
    let end = now() + timeout;
    loop {
        if let Some(at) = ctx
            .journal()
            .iter()
            .skip(from)
            .position(|(_, r)| typed_line(&rec_s(r, "line")))
        {
            ctx.mark("journaled the wake's line", None);
            return Ok(Some(from + at));
        }
        if now() >= end {
            ctx.put("error", "no wake line journaled typed");
            return Ok(None);
        }
        stop::sleep(std::time::Duration::from_millis(200))?;
    }
}

/// The flag's folder `dir` laid with no flag in it. The agent under test may write under its
/// home and the lab writes there as its owner, outside the jail, so nothing here follows a link:
/// a link on the way refuses the run.
///
/// # Errors
/// A part of `dir` is a link or could not be made, or the old flag could not be removed.
pub fn clear_flag(dir: &Path) -> std::io::Result<()> {
    crate::sys::scratch_dirs(dir)?;
    crate::sys::scratch_remove(&dir.join(FLAG))
}

/// The flag touched in `dir`: a new empty file renamed into place ([`crate::sys::put`]),
/// so a link the agent left at its name is replaced, never written through, and one on the way
/// refuses it ([`clear_flag`]'s reason).
///
/// # Errors
/// A relative path, parent traversal, a link in `dir`, or the write or rename failing.
pub fn touch_flag(dir: &Path) -> std::io::Result<()> {
    crate::sys::put(&dir.join(FLAG), 0o644, &crate::sys::PutSrc::Bytes(b""))
}

/// THE RUN.
///
/// # Errors
/// Interrupted, or the lab refused the settings, the script or the flag's folder.
pub fn play(ctx: &mut Ctx) -> Result<(), ScnErr> {
    if ctx.standin() {
        ctx.put(crate::occupancy_standin::SUMMARY_KEY, J::Bool(true));
    }
    let run = format!("{NAME}-{:02}", ctx.k);
    let dir = ctx.run.w.join("home").join(flag_dir(&run));
    let flag = dir.join(FLAG);
    clear_flag(&dir).map_err(|e| ScnErr::from(format!("{}: {e}", dir.display())))?;
    crate::verbs::settings_merge(&ctx.run, &allowed())?;
    crate::verbs::set_tool(&ctx.run, &script(&run).dump(COMPACT))?;
    let since = ctx.n_frames();
    let from = ctx.journal().len();
    ctx.turn(TURN)?;
    let first = ctx.wait_for(
        |f| workflow_waits(&f.rows) || f.rows.iter().any(|r| r.contains(WORKFLOW_BOX)),
        WAITS_WITHIN_S,
        since,
    )?;
    let Some(first) = first else {
        ctx.put("error", "the workflow's wait was never drawn");
        return Ok(());
    };
    if !workflow_waits(&first.rows) {
        ctx.put("workflow_box", "answered as a person");
        ctx.ctl(&["key", "enter"]);
        if ctx
            .wait_for(|f| workflow_waits(&f.rows), WAITS_WITHIN_S, first.i)?
            .is_none()
        {
            ctx.put("error", "the workflow's wait was never drawn past its box");
            return Ok(());
        }
    }
    ctx.mark("workflow waits", None);
    let at_sid = format!("@{}", ctx.sid);
    let waits_before = ctx.ctl(&[&at_sid, "waits"]).stdout;
    touch_flag(&dir).map_err(|e| ScnErr::from(format!("{}: {e}", flag.display())))?;
    ctx.mark("flag touched", None);
    let Some(typed) = typed_after(ctx, from, told_within_s())? else {
        return Ok(());
    };
    let typed_seen = ctx.journal().get(typed).map_or(0.0, |(seen, _)| *seen);
    let _ = ctx.wait_for(
        |f| f.rows.iter().any(|r| r.contains("I keep waiting on them")),
        ANSWERED_WITHIN_S,
        0,
    )?;
    stop::sleep(std::time::Duration::from_secs(5))?;
    let journal: Vec<String> = ctx
        .journal()
        .iter()
        .skip(from)
        .map(|(_, r)| rec_s(r, "line"))
        .collect();
    let frames = ctx.frames();
    let at_write = before_line(&frames, typed_seen);
    let after: Vec<Vec<String>> = frames
        .into_iter()
        .filter(|f: &Frame| f.wall >= typed_seen)
        .map(|f| f.rows)
        .collect();
    let ledger_path =
        aterm_agent::supervise::approvals::ledger_under(&ctx.run.state(), Some(&ctx.sid));
    let ledger = crate::util::scratch_text(&ledger_path);
    let status = ctx.ctl(&[&at_sid, "status"]).stdout;
    let waits_after = ctx.ctl(&[&at_sid, "waits"]).stdout;
    let messages = ctx.ctl(&["messages", "tag=harness"]).stdout;
    let fail = judge(&Recording {
        sid: &ctx.sid,
        run: &run,
        status: &status,
        waits_before: &waits_before,
        waits_after: &waits_after,
        journal: &journal,
        ledger: &ledger,
        messages: &messages,
        at_write: at_write.as_deref(),
        after: &after,
    });
    ctx.put(
        "occupancy_lines",
        J::Arr(
            journal
                .iter()
                .filter(|l| l.starts_with("OCCUPANCY ") || l.starts_with("FACT "))
                .map(J::s)
                .collect(),
        ),
    );
    ctx.put(
        "occupancy_judged",
        if fail.is_empty() {
            J::s("pass")
        } else {
            J::Arr(fail.iter().map(J::s).collect())
        },
    );
    if fail.is_empty() {
        ctx.sumd.remove("error");
    } else {
        ctx.put("error", fail.join("; "));
    }
    Ok(())
}

#[cfg(test)]
#[path = "occupancy_wake_tests.rs"]
mod tests;
