// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `occupancy-break-claude`: THE OCCUPANCY DOOR ON A REAL CLAUDE CODE (harness occupancy, slice 1;
//! `docs/DESIGN-harness-occupancy-2026-10-20.md` §8). Under the owner's `occupancy = "prompt"` (the
//! lab's scratch `aterm.toml`, the one place an agent may write it) and the nudge's wait cut to a
//! minute ([`NUDGE_SEAM`](aterm_agent::supervise::occupancy::rules::NUDGE_SEAM), which only a build
//! that reads seams reads), Claude Code is asked to start a background workflow of two `sleep 900`
//! steps and wait on it. The scenario holds that, at the break it waits at:
//!
//! * the harness types exactly one `[aterm harness] … (rule wait-nudge@v1)` line, fenced (the
//!   door books `typed` only after a fenced write; an unfenced read is `skipped why=unfenced`);
//! * the workflow is still running after it;
//! * the agent's `WAITING:` answer is read as the answer to ours (`hold declared`);
//! * the journal, the ledger and Messages say so as §6 describes: a `fact-intent@v1` row before
//!   the line's `fact-hold@v1` `typed` row, one episode, held at least the nudge's wait, no
//!   `fact-would@v1` row; the journal's `hold open`, one `door=break typed`, then `hold declared`,
//!   with no `hold closed` after; and the session's supervisor record of the line
//!   (`harness.supervisor.<sid>`, the host's `inform`) ([`judge`]).
//!
//! The lab's stub plays the agent ([`script`]): the main thread calls `Workflow`, says it started,
//! then answers the nudge with a `WAITING:` line; each workflow step's subagent, asked under
//! [`STEP_MARK`], calls `Bash` `sleep 900` (Claude Code's Bash ends it at its 10 min ceiling, past
//! the scenario's window) and says it slept. Recorded with `lab scn run occupancy-break-claude`;
//! its run promotes into the tape corpus with `lab tape promote occupancy-break-claude-01`, which
//! refuses a run the judge failed (its `error`).

use aterm_agent::supervise::facts::FactKind;
use aterm_agent::supervise::occupancy::door::INTENT_RULE;
use aterm_agent::supervise::occupancy::rules::{RuleId, WOULD_RULE};
use aterm_agent::supervise::occupancy::templates;

use crate::pyjson::{COMPACT, J};
use crate::record::{Frame, rec_s};
use crate::scn::{Ctx, ScnErr, journaled_after};
use crate::stop;

/// The scenario's name (`lab scn run`; its runs `<name>-NN`, its corpus tape `<name>`).
pub const NAME: &str = "occupancy-break-claude";

/// Its `[harness]` lines, under the lab's root `[harness]` table: the owner's `prompt`, the
/// shipped `cruise` (the nudge is cruise's alone), and a 5 s grace so the lab's own typing of the
/// task is past before the minute's nudge is due.
pub const HARNESS_KEYS: &str = "occupancy = \"prompt\"\nautopilot = \"cruise\"\nhuman_grace_s = 5";

/// The nudge's wait the seam sets, in seconds.
pub const NUDGE_S: u32 = 60;

/// The lab instance's own variables: the nudge's seam
/// ([`NUDGE_SEAM`](aterm_agent::supervise::occupancy::rules::NUDGE_SEAM)) at [`NUDGE_S`].
pub const INSTANCE_ENV: &str = "ATERM_OCCUPANCY_NUDGE_S=60";

/// What a workflow step's subagent is asked under: its first message holds it, and the stub
/// answers that conversation from the steps that name it (`when`), never the main thread's.
pub const STEP_MARK: &str = "occupancy-lab-step";

/// The workflow the agent starts (the `Workflow` tool's `script`): two steps at once, each a
/// subagent asked under [`STEP_MARK`] to sleep.
pub const WORKFLOW: &str = "export const meta = { name: 'occupancy-lab', description: 'Two sleeps the occupancy lab waits on' }\n\
                            await parallel([1, 2].map((n) => () => agent(`occupancy-lab-step ${n}: run sleep 900 in Bash, then say slept.`)))\n";

/// The task the lab types: the stub's tool mode starts at a message holding `toolstart`.
pub const TURN: &str =
    "toolstart: start a background workflow of two sleep 900 steps and wait on it";

/// The main thread's word once the workflow runs: a plain status, no question to the person.
pub const STARTED: &str = "Started the occupancy-lab workflow: its two sleep steps run in the background, and I am waiting on them.";

/// The main thread's answer to the nudge: the declaration the `WAITING:` reader takes.
pub const WAITING: &str =
    "WAITING: the occupancy-lab workflow's two sleep 900 steps; they unblock no other step.";

/// A step's last word.
const SLEPT: &str = "slept";

/// The `Bash` call's timeout: Claude Code's ceiling, 10 min.
const BASH_TIMEOUT_MS: i64 = 600_000;

/// Claude Code's box before a workflow runs, where a build asks it.
const WORKFLOW_BOX: &str = aterm_phase::anchors::anchor_text("box.workflow");

/// A hang detector for the workflow's wait to be drawn after the task is typed.
const WAITS_WITHIN_S: f64 = 120.0;

/// The nudge's hang detector past [`NUDGE_S`]: the door's look, a minute apart, and a minute
/// more.
const TYPED_PAST_S: f64 = 120.0;

/// A hang detector for the agent's `WAITING:` answer to be read (the stub answers at once).
const DECLARED_WITHIN_S: f64 = 60.0;

/// The stub's tool script ([module docs](self)).
#[must_use]
pub fn script() -> J {
    let step = |pairs: Vec<(&str, J)>| {
        let mut o = J::obj([("when", J::s(STEP_MARK))]);
        for (k, v) in pairs {
            o.set(k, v);
        }
        o
    };
    J::Arr(vec![
        J::obj([
            ("name", J::s("Workflow")),
            ("input", J::obj([("script", J::s(WORKFLOW))])),
        ]),
        J::obj([("text", J::s(STARTED))]),
        J::obj([("text", J::s(WAITING))]),
        step(vec![
            ("name", J::s("Bash")),
            (
                "input",
                J::obj([
                    ("command", J::s("sleep 900")),
                    ("description", J::s("Sleep for the occupancy lab")),
                    ("timeout", J::Int(BASH_TIMEOUT_MS)),
                ]),
            ),
        ]),
        step(vec![("text", J::s(SLEPT))]),
    ])
}

/// The scratch settings' allow rules: the workflow and its sleeps run without a box.
fn allowed() -> J {
    J::obj([(
        "permissions",
        J::obj([(
            "allow",
            J::Arr(
                ["Workflow", "Bash(sleep:*)", "Bash(sleep *)"]
                    .into_iter()
                    .map(J::s)
                    .collect(),
            ),
        )]),
    )])
}

/// Whether `rows` draw the agent waiting on a workflow of its own: the reader's background
/// wait, a workflow's rule.
#[must_use]
pub fn workflow_waits(rows: &[String]) -> bool {
    matches!(
        aterm_phase::phase::background_wait(rows),
        Some("waiting for a dynamic workflow" | "a workflow running")
    )
}

/// What the judge reads of one run, all of it after the task was typed.
#[derive(Debug, Clone, Copy)]
pub struct Recording<'a> {
    /// The session's id.
    pub sid: &'a str,
    /// The session's `status` reply.
    pub status: &'a str,
    /// The session's journal lines, oldest first.
    pub journal: &'a [String],
    /// The session's ledger text.
    pub ledger: &'a str,
    /// The instance's `messages tag=harness` reply.
    pub messages: &'a str,
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

/// The session `sid`'s rows of the ledger text `ledger`, oldest first.
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

/// The value of `key=` among `text`'s words.
fn word<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.split(' ')
        .find_map(|w| w.strip_prefix(key)?.strip_prefix('='))
}

/// The key of the record a typed line leaves for session `sid` (the host's `inform`).
fn record_key(sid: &str) -> String {
    format!("harness.supervisor.{}", sid.trim_start_matches('@'))
}

/// The decoded titles of the `messages` rows (`messages`, the wire's `message …` lines) keyed
/// `key`, oldest first.
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

/// THE SCENARIO'S VERDICT on `r` ([module docs](self)): each way the run fell short of it, in a
/// line; none for a pass.
#[must_use]
pub fn judge(r: &Recording<'_>) -> Vec<String> {
    let mut fail = Vec::new();
    if !r.status.split_whitespace().any(|w| w == "occupancy=prompt") {
        fail.push("status does not say occupancy=prompt".to_string());
    }
    let nudge = RuleId::WaitNudge.name();
    let hold = FactKind::Hold.rule();
    let rows = rows_of(r.ledger, r.sid);
    let would = rows.iter().filter(|w| w.rule == WOULD_RULE).count();
    if would > 0 {
        fail.push(format!("{would} {WOULD_RULE} row(s) under prompt"));
    }
    let skipped = rows
        .iter()
        .filter(|w| w.rule == hold && w.decision == "skipped")
        .count();
    if skipped > 0 {
        fail.push(format!("{skipped} {hold} skipped row(s)"));
    }
    let typed: Vec<(usize, &Row)> = rows
        .iter()
        .enumerate()
        .filter(|(_, w)| w.rule == hold && w.decision == "typed")
        .collect();
    let intents: Vec<(usize, &Row)> = rows
        .iter()
        .enumerate()
        .filter(|(_, w)| w.rule == INTENT_RULE)
        .collect();
    let mut episode = None;
    match (typed.as_slice(), intents.as_slice()) {
        ([(at, row)], [(intent_at, intent)]) => {
            let held = word(&row.reason, "held_ms").and_then(|v| v.parse::<i64>().ok());
            if word(&row.reason, "rule") != Some(nudge) {
                fail.push(format!("the typed row is no {nudge} row: {}", row.reason));
            } else if held.is_none_or(|ms| row.command != templates::wait_nudge(ms).text()) {
                fail.push(format!(
                    "the typed row's line is not {nudge}'s for its hold: {:?}",
                    row.command
                ));
            }
            if let Some(ms) = held {
                if ms < i64::from(NUDGE_S) * 1000 {
                    fail.push(format!(
                        "the typed row's hold is under the nudge's {NUDGE_S} s wait: {ms} ms"
                    ));
                }
                let fact = templates::wait_nudge(ms);
                let key = record_key(r.sid);
                if !titles_keyed(r.messages, &key)
                    .iter()
                    .any(|title| title.contains(fact.fact()))
                {
                    fail.push(format!("no {key} record of the line in Messages"));
                }
            }
            if intent.decision != "waiting" || word(&intent.reason, "rule") != Some(nudge) {
                fail.push(format!("the intent row is no waiting {nudge} row"));
            }
            if intent_at > at {
                fail.push("the intent row follows the typed row".to_string());
            }
            episode = word(&row.reason, "episode");
            if word(&intent.reason, "episode") != episode {
                fail.push("the intent and the typed row are two episodes".to_string());
            }
        }
        _ => fail.push(format!(
            "{} {hold} typed row(s) and {} {INTENT_RULE} row(s), not one of each",
            typed.len(),
            intents.len()
        )),
    }
    let occupancy = |l: &&String| l.starts_with("OCCUPANCY ");
    let lines: Vec<&String> = r.journal.iter().filter(occupancy).collect();
    let ours = |l: &str| word(l, "rule") == Some(nudge);
    let typed_lines: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| ours(l) && l.ends_with(" door=break typed"))
        .map(|(i, _)| i)
        .collect();
    if lines
        .iter()
        .any(|l| ours(l) && l.contains(" door=break would"))
    {
        fail.push("the journal says a would-be line under prompt".to_string());
    }
    let [at] = typed_lines.as_slice() else {
        fail.push(format!(
            "{} journal line(s) say the nudge typed, not one",
            typed_lines.len()
        ));
        return fail;
    };
    let at = *at;
    if episode.is_some() && word(lines[at], "episode") != episode {
        fail.push("the journal's typed line is not the ledger's episode".to_string());
    }
    if !lines[..at].iter().any(|l| l.contains(" hold open")) {
        fail.push("no hold open before the line".to_string());
    }
    if !lines[at + 1..]
        .iter()
        .any(|l| l.contains(" hold declared "))
    {
        fail.push(
            "the agent's WAITING: answer was not read as ours (no hold declared)".to_string(),
        );
    }
    if lines[at + 1..].iter().any(|l| l.contains(" hold closed ")) {
        fail.push("the hold closed after the line".to_string());
    }
    if !r.after.last().is_some_and(|rows| workflow_waits(rows)) {
        fail.push("the last screen after the line draws no running workflow".to_string());
    }
    fail
}

/// THE RUN ([module docs](self)): the settings' allow rules and the stub's script, the task, the
/// workflow's wait drawn (its box answered as a person would, where this build asks one, long
/// before the minute's hold is due), the line typed and the answer read under hang detectors, and
/// [`judge`]'s verdict in the summary (`occupancy_judged`; `error` where it failed).
/// # Errors
/// Interrupted, or the lab refused the settings or the script.
pub fn play(ctx: &mut Ctx) -> Result<(), ScnErr> {
    crate::verbs::settings_merge(&ctx.run, &allowed())?;
    crate::verbs::set_tool(&ctx.run, &script().dump(COMPACT))?;
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
    let typed_within = f64::from(NUDGE_S) + TYPED_PAST_S;
    let Some(typed) = journaled_after(ctx, from, " door=break typed", typed_within)? else {
        return Ok(());
    };
    let typed_seen = ctx.journal().get(typed).map_or(0.0, |(seen, _)| *seen);
    // The judge says whether it came: its own `error` stands for this wait's.
    let _ = journaled_after(ctx, typed + 1, " hold declared ", DECLARED_WITHIN_S)?;
    stop::sleep(std::time::Duration::from_secs(5))?;
    let journal: Vec<String> = ctx
        .journal()
        .iter()
        .skip(from)
        .map(|(_, r)| rec_s(r, "line"))
        .collect();
    let after: Vec<Vec<String>> = ctx
        .frames()
        .into_iter()
        .filter(|f: &Frame| f.wall >= typed_seen)
        .map(|f| f.rows)
        .collect();
    let ledger_path =
        aterm_agent::supervise::approvals::ledger_under(&ctx.run.state(), Some(&ctx.sid));
    let ledger = crate::util::scratch_text(&ledger_path);
    let status = ctx.ctl(&["status"]).stdout;
    let messages = ctx.ctl(&["messages", "tag=harness"]).stdout;
    let fail = judge(&Recording {
        sid: &ctx.sid,
        status: &status,
        journal: &journal,
        ledger: &ledger,
        messages: &messages,
        after: &after,
    });
    ctx.put(
        "occupancy_lines",
        J::Arr(
            journal
                .iter()
                .filter(|l| l.starts_with("OCCUPANCY "))
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
#[path = "occupancy_break_tests.rs"]
mod tests;
